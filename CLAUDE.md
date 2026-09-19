# screeny

Custom Rust firmware that turns a Gen 1 Tidbyt (ESP32 + 64x32 HUB75 panel) into a
network frame buffer, plus host-side Rust tools that stream frames to it. It works end
to end (see `README.md` and `docs/research/005-end-to-end.md`). Current phase:
**cleanup and consolidation**, and the generative art system (`crates/art`, `crates/studio`) becoming
the primary sender; see `docs/design/studio-vision.md`.

Read `README.md` for the map, then `docs/README.md` for how the kanban board and
workers operate. `docs/design/` is the source of truth (`protocol-v1.md`,
`architecture.md`, `generative-art-brief.md`); `docs/research/` is how we got there.

## Ground rules

- **Rust everywhere.** Firmware is `no_std` embassy / esp-hal on esp-rtos. Host tools
  are std Rust on the stable toolchain, all in the one root cargo workspace
  (`crates/*`). `firmware/` and `lab/` are separate cargo projects. A plain
  `cargo test` skips only `crates/studio` (Tauri, until card 105); build it with `-p`.
- **One implementation of each thing.** Wire format and decoders live in
  `crates/proto` and nowhere else. If you need wire logic, add it there.
- **Target: 30 fps**, one frame per UDP datagram (<= 1472 byte payload, 1464 for
  pixels). Headroom goes to image quality, not more fps.
- **The spec is normative.** If code and `docs/design/protocol-v1.md` disagree, that
  is a bug in one of them: fix it and say which in the commit.
- **Hardware is single-owner.** One Tidbyt on `/dev/cu.usbserial-2140`, one camera.
  Only the orchestrator, or a worker whose card says `hardware: yes`, may flash, open
  the serial port, or capture from the camera. Everyone else uses `crates/sim`.
  Serial baud never above 230400 on this bench; it corrupts.
- **Never flash** unless `backup/tidbyt-stock-*.bin` exists (8388608 bytes). Never
  erase flash. Flash with `tools/fw-run.sh`.
- **Bench discipline.** Separate fast checks from long evidence runs: iterate with
  host tests and the simulator, run long device tests once on the final build, never
  repeat a passing long run without a firmware change. Put a timeout on every
  emulator, server or monitor you start; cap anything that logs per event; leave no
  background processes behind. (A forgotten qemu trace once wrote 196 GB.)
- **Panel brightness is capped in firmware.** The panel runs off laptop USB. No
  full-white full-brightness frames, no flashing above 3 Hz.
- The device: `192.168.7.221`, mDNS host `screeny-4a00a4.local`, instance
  `screeny-4a00a4`, frames UDP 49374, control UDP 49375. This unit's HUB75 colour
  lines are rotated relative to Tidbyt's published pin map (fixed in `firmware/`).
- WiFi credentials are **not in git**. `firmware/build.rs` compiles them in from
  the environment, `firmware/wifi.env` (gitignored) or `~/.config/screeny/wifi.env`
  (outside the repo, so every worktree finds it). Never write the real SSID or
  password into a tracked file, a card log, a commit message, a test fixture or a
  worker prompt; tests use the dummies `Example-Wifi1` / `password9`. The plan for
  provisioning is a captive-portal setup with an HTTP settings page; do not build
  other schemes.
- Camera captures are for "is it showing the right thing", not colour measurement:
  the bench camera's colour response is unknown and it cannot photograph the
  temporally dithered panel honestly. The Claude app has no camera permission;
  captures go through `tools/cam-request.sh` (daemon in Terminal.app).
- Xtensa toolchain: `. ~/export-esp.sh`, then build in `firmware/` (its
  `rust-toolchain.toml` selects `esp`).
- macOS: build the sender with `tools/sign-macos.sh` when it will be launched outside
  a terminal (Developer ID signing keeps Local Network permission stable).
- Workers work in their own git worktree/branch, log as they go, commit after every
  step, and do not merge to `main`; the orchestrator merges.
- Remote: `origin` = `git@github.com:gotwalt/screeny.git` (private, currently empty).
  **Nobody pushes until the owner says so**; all work stays local. Workers never push.
  The repo is intended to become public: keep it free of credentials and of anything
  the owner has not chosen to publish.
