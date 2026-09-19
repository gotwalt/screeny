---
id: 107
title: docker-compose.yml and deployment to workbench.local
type: build
hardware: no
depends: [105]
owner:
branch:
---

## Goal

`docker compose up -d` on `workbench.local` runs `screeny-studio` against the real
panel, GPU pieces included, and it comes back by itself after a reboot. Deployable as
a Portainer stack. Target facts: `docs/design/studio-vision.md`, "Deployment target".

## Deliverables

- `Dockerfile`: multi-stage; builder with the stable Rust toolchain and a cargo cache
  mount; runtime on a slim Debian/Ubuntu base with `mesa-vulkan-drivers` +
  `libvulkan1` (Intel ANV for wgpu), CA certs, tzdata; non-root user; the UI embedded
  in the binary so the image is one file + libs.
- `docker-compose.yml`: `network_mode: host`; `devices: [/dev/dri:/dev/dri]`;
  `group_add` for the host's `render` gid (993 on workbench; make it an env var);
  `TZ`; a named volume for state; `restart: unless-stopped`; healthcheck on
  `/healthz`; json-file logging with `max-size`/`max-file`; `WGPU_BACKEND=vulkan`;
  web port 8787 (free on workbench; configurable).
- A second profile / override file for hosts without host networking or a GPU
  (macOS Docker, other people's machines): published port, manual device addresses,
  CPU pieces only. It must start and stream; discovery is expected not to work there.
- `tools/deploy-workbench.sh`: rsync or `git pull` + `docker compose build && up -d`
  over SSH, prints `/healthz`. Building inside Portainer is too slow for a Rust + wgpu
  image; Portainer still sees and manages the stack.
- Verify early and write down: (a) `mdns-sd` inside a host-network container coexists
  with the host's avahi on UDP 5353 and finds `screeny-4a00a4`; (b) wgpu enumerates
  the Intel adapter inside the container and a GPU piece renders; (c) the clocks show
  the right local time.
- The orchestrator owns anything that changes workbench itself (it hosts other
  services: Portainer, scrypted, the homework stack, Ollama on 11434). Do not touch
  other containers, do not change Docker daemon settings, do not open firewall ports.

## Acceptance

After `docker compose up -d` and a host reboot, the panel shows the configured piece
without anyone logging in; `docker compose ps` reports healthy; Portainer shows the
stack.

## Log
