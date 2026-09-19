# screeny

Custom Rust firmware that turns a Gen 1 Tidbyt (ESP32 + 64x32 HUB75 panel) into a
network frame buffer, plus host-side Rust tools that stream frames to it.

Read `docs/README.md` first: it explains the kanban board, how workers pick up
cards, and the hardware access rules. `docs/design/` holds decisions that are
settled; `docs/research/` holds findings that feed them.

## Ground rules

- **Rust everywhere.** Firmware is `no_std` embassy / esp-hal. Host tools are std Rust.
- **Target: 30 fps**, one frame per UDP datagram (<= 1472 byte payload). Headroom goes
  to image quality, not more fps.
- **Hardware is single-owner.** There is one Tidbyt on `/dev/cu.usbserial-2140` and one
  camera. Only the orchestrator, or a worker whose card says `hardware: yes`, may flash,
  open the serial port, or capture from the camera. Everyone else tests against the
  host simulator. Never use baud rates above 230400 on this port; it corrupts.
- **Never flash** unless `backup/tidbyt-stock-*.bin` exists (8388608 bytes).
- **Panel brightness is capped in firmware.** The panel runs off laptop USB.
- WiFi: SSID `example-wifi1`, password `password9` (owner says this is not secret).
- Xtensa toolchain: `. ~/export-esp.sh` then build with the `esp` rustup toolchain.
- Workers work in their own git worktree/branch and do not merge to `main`; the
  orchestrator merges.
