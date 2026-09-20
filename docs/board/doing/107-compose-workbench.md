---
id: 107
title: docker-compose.yml and deployment to workbench.local
type: build
hardware: no
depends: [105]
owner: worker-107
branch: card/107-compose-workbench
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
- `tools/deploy-workbench.sh`: over SSH, clone/pull `git@github.com:gotwalt/screeny.git`
  (private; workbench already authenticates to GitHub as `gotwalt` and can read it -
  verified 2026-09-19) into `~/src/screeny`, then `docker compose build && up -d`,
  then print `/healthz`. Deploys what is on `origin/main`, so it refuses to run if the
  local `main` is ahead of `origin/main` (deploying unpushed work silently would be a
  trap). The owner is keeping the repo local-only until it is ready to publish, so
  support a second route that needs no GitHub: a bare repo on workbench reached as an
  SSH git remote (`git push workbench main`), with the deploy script building from a
  checkout of that. Pick by flag; same refusal-to-deploy-unpushed-work rule. Building inside Portainer is too slow for a Rust + wgpu
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

### Decisions taken as given (from the orchestrator)

- The state volume is mounted at **`/data`**; the compose files set
  `SCREENY_STATE_DIR=/data` and `SCREENY_LISTEN=0.0.0.0:8787`. The binary does not
  read either yet - card 106 adds the fallbacks - so the port is *also* passed as
  `--listen` in `command:`. Both compose files carry a comment saying to delete the
  `command:` line once 106 lands.
- Port from `${SCREENY_PORT}`, default 8787.
- No auth on the studio; it listens on the LAN by the owner's choice; do not publish
  it beyond the host. Said plainly at the top of `docs/design/deployment.md`.
- The deploy script only ever runs `git` (read-only, locally), `ssh`, and
  `docker compose` with `-p screeny` and an explicit `-f`, inside its own checkout
  directory on the host. It prints every command before running it; `--dry-run`
  prints and runs nothing. No prunes, no daemon settings, no firewall, no other
  container.
- `TZ` defaults to `America/Los_Angeles` and the image installs `tzdata`.

**Git route changed mid-card (2026-09-20, owner, relayed by the orchestrator):** the
owner allowed pushing `main` to the private `origin` on GitHub, so the **GitHub route
is now the script's default** and the bare-repo-on-workbench route lives behind
`--route workbench`. The script itself still never pushes anything anywhere: it
refuses when the branch is not on the chosen remote and prints the push to run.

### Step 1 - the image

`Dockerfile`, two stages.

**Builder** is `rust:1-trixie`. `rustup toolchain install stable` is its own layer
before any source is copied: the workspace's `rust-toolchain.toml` says
`channel = "stable"`, which is *not* the name the official image installs the
compiler under, so without that layer rustup would download a whole toolchain inside
the build step and do it again on every source change. Then the manifests, then
`crates/`, then one `RUN` with three BuildKit cache mounts - cargo's `registry`, its
`git`, and the workspace `target/`. The binaries have to be copied out inside that
same `RUN`: a cache mount is not part of the layer, so anything left in
`/src/target` is gone when the step ends. `--locked`, so the image is always built
from the `Cargo.lock` in the tree.

**Runtime** is `debian:trixie-slim` plus `libvulkan1`, `mesa-vulkan-drivers`,
`vulkan-tools`, `curl`, `ca-certificates`, `tzdata`. Non-root: uid/gid 10001
`studio`, owning `/data` so an empty named volume mounted there inherits that
ownership. Access to `/dev/dri` deliberately does *not* come from the image - the
render gid is a property of the host, so it is `group_add` in the compose file.

Two binaries go in, not one: `screeny-studio` and the `screeny` CLI. The CLI is 2 MB
and it is how "does mDNS work in this container?" gets a one-line answer
(`docker exec screeny-studio screeny discover`), plus `stats`/`brightness` against
the panel from inside the container that is actually driving it.

`FEATURES` build arg: empty (default) = the crate defaults, which include `gpu`;
`none` = `--no-default-features`, a studio with no graphics driver compiled in at
all. Proven below.

`.dockerignore` keeps `target/`, `.git/`, `docs/`, `.claude/`, `captures/`, `lab/`,
`tools/` out of the context, and **excludes `firmware/` and `backup/` whole** - which
is also how `firmware/wifi.env` is kept out of every layer. `**/wifi.env`, `.env` and
`*.env` are excluded again by name, belt and braces. The build compiles host crates
only and needs no credential of any kind.

**Measured on this Mac** (arm64, colima, Docker 29.8.1 client / 29.5.2 engine):

| | |
|---|---|
| cold build, empty caches | **54 s** wall (`docker compose build`), of which cargo 40 s (164 crates, `Finished release in 39.5 s`), apt+base 7 s, export 6 s |
| rebuild, nothing changed | **1.2 s** (pure layer cache hit) |
| rebuild, `FEATURES=none`, warm caches | **11.9 s** (cargo re-links against a warm registry and `target/`) |
| image, default (gpu) | **491 MB** unpacked |
| image, `FEATURES=none` | **479 MB** - barely smaller, because the runtime stage installs Mesa either way |
| layer breakdown | debian base 110 MB, the apt layer 253 MB, `screeny-studio` 11.8 MB, `screeny` 2.9 MB |

**Stripping, measured rather than guessed.** The workspace builds release with
`debug = 1`, and the three options are not close:

| | `screeny-studio` | `screeny` |
|---|---|---|
| untouched | 77.7 MB | 23.1 MB |
| `strip --strip-debug` | **11.8 MB** | **2.9 MB** |
| `strip` (everything) | 8.1 MB | 2.1 MB |

So keeping the DWARF costs ~90 MB and keeping only `.symtab` costs 3.7 MB. The image
does `--strip-debug`: a panic backtrace from a service nobody is watching still names
its functions, for 3.7 MB, and only the line numbers are gone.

The apt layer is the whole story on size: Debian ships `mesa-vulkan-drivers` as one
package with every driver in it (ANV, RADV, nouveau, panvk, lavapipe, ...) and there
is no Intel-only split. Dropping `vulkan-tools` would save a couple of MB; making the
Mesa install conditional on `FEATURES` would save ~250 MB on a CPU-only image, at the
cost of a second way for the runtime stage to be wrong. Not worth it yet - noted here
so the next person does not have to re-derive it.

### Step 2 - the compose files

`docker-compose.yml` is the Linux/GPU one: `network_mode: host`,
`devices: [/dev/dri:/dev/dri]`, `group_add: ["${SCREENY_RENDER_GID:-993}"]`,
`WGPU_BACKEND=vulkan`, `TZ`, a named `state` volume on `/data`,
`restart: unless-stopped`, a `curl /healthz` healthcheck (30 s interval, 30 s
`start_period`, 3 retries), `stop_grace_period: 20s`, and json-file logging capped at
`10m` x 3. `container_name: screeny-studio` so `docker exec screeny-studio ...` is a
thing you can type. `name: screeny` at the top, so the project name is right even if
someone forgets `-p`.

`docker-compose.portable.yml` is a **complete file, not an override**, and that is
the interesting part: compose merges list-valued keys like `devices:` and
`group_add:` by *appending*, so an override can add a device but can never take one
away. `-f docker-compose.yml -f override.yml` would still try to open `/dev/dri` on a
Mac. Both files say so in a comment.

### Step 3 - what was proven here, on the portable profile

`docker compose -p screeny107 -f docker-compose.portable.yml up -d`, all inside a
bounded session, everything removed afterwards.

- **Starts as non-root**: `uid=10001(studio) gid=10001(studio) groups=10001(studio)`.
- **`/healthz` -> `200 ok`**, and compose reports `Up (healthy)` ~20 s after start.
- **UI is served from the binary**: `/` 7658 B, `/main.js` 23454 B, `/style.css`
  9737 B, all 200. `/api/v1/bootstrap` 8343 B, `/api/v1/frame` exactly 6196 B.
- **Streams to a host `screeny-sim`.** Sim on the Mac, headless, `--no-mdns`,
  `--exit-after 60`; studio in the container pointed at the host by address. The sim
  logged **29.7-30.5 fps, PAL8_LZ, ~400-750 B/frame, `gaps 0 stale 0 dec 0 rej 0`**
  for the whole run, and the studio reported `state: up, indexed_exact: 180,
  indexed_fallback: 0`. So the container's outbound UDP path is exact-indexed and
  holds the frame rate.
- **SIGTERM stops it cleanly, fast.** `docker compose stop` returned in **0.49 s**
  (grace period 20 s), exit code **0**, and the simulator logged
  `lock released by ...: Final` / `state Live -> Hold` - the panel is let go at once
  rather than held on the last frame until the stream times out.
- **The state volume is writable by the non-root user and survives a restart**:
  `/data` is `drwxr-xr-x studio studio`, a file written there was still there after
  `docker compose restart`, volume `screeny107_state`.
- **Logs are capped**: `{"Type":"json-file","Config":{"max-file":"3","max-size":"10m"}}`.
  Restart policy `unless-stopped`. Container user `studio`.
- **Vulkan works inside the container.** `vulkaninfo --summary` found
  `GPU0 ... llvmpipe (LLVM 19.1.7, 128 bits)`, `PHYSICAL_DEVICE_TYPE_CPU` - Mesa's
  software rasteriser, which is what there is to find on a Mac VM with no `/dev/dri`.
  Selecting the `overland` piece then logged
  **`screeny-art: gpu=llvmpipe (LLVM 19.1.7, 128 bits) backend=Vulkan`** - exactly the
  line to grep for on workbench, where it should name Intel instead. So the wgpu ->
  Vulkan -> Mesa path is live in the image; on workbench only the *adapter* changes.
- **`FEATURES=none` really is CPU-only.** Its `/api/v1/bootstrap` lists
  `clocks-numerals, clocks-dials, plasma, metaballs, testcard`; the default image
  lists those plus `overland, lattice, knot`.

**Surprise worth writing down: `set_panel` needs a literal address, not a hostname.**
`{"to":"host.docker.internal:49374"}` does not parse as a `SocketAddr`, so the link
treated it as an mDNS *instance name*, browsed `_screeny._udp.local.` for 3 s, found
nothing, and dropped 361 frames while reporting
`last_error: no screeny device found`. `{"to":"192.168.5.2:49374"}` (what
`host.docker.internal` resolves to inside the container) worked immediately. This is
not a blocker - the real target is an IP - but a name that is neither an instance nor
an address currently fails in a way that reads like "the panel is missing" rather
than "that is not an address I can use". Written up as **card 146**.

### Step 4 - `tools/deploy-workbench.sh`

One script, printing everything, refusing rather than guessing.

- `--route github` (default): the host clones/pulls
  `git@github.com:gotwalt/screeny.git` into `~/src/screeny`. `--route workbench`:
  a bare repo at `~/srv/screeny.git` on the host, reached as the local git remote
  `workbench` (`workbench.local:srv/screeny.git`), which the script will *add* if
  missing but will never *repoint*, and refuses outright if that remote already
  points at GitHub.
- **It never pushes.** It fetches, compares the branch with `<remote>/<branch>`, and
  if local is ahead it refuses with `git push <remote> <branch>` and
  `"this script never pushes for you; that is a decision, not a step"`. If local is
  *behind* it warns and carries on, saying the host gets the remote's commit.
- Every compose invocation is
  `cd $DIR && SCREENY_PORT=.. SCREENY_RENDER_GID=.. TZ=.. docker compose -p screeny -f docker-compose.yml ...`.
  Five other compose projects run on that host (`docker`, `homework`,
  `homework-work`, `june`, `scrypted`), and the project name is what keeps this one
  out of their way.
- `--dry-run`, `--status`, `--logs`, `--down`, `--down --volumes`, `--no-build`,
  `--host`, `--dir`, `--bare`, `--port`, `--render-gid`, `--tz`, `--branch`.
- After a successful deploy it prints the three verification one-liners.

**A bug the dry run caught**, worth recording: the first version emitted
`ssh host -- SCREENY_PORT=8787 cd ~/src/screeny && docker compose ...`. `cd` is a
regular builtin, so a prefix assignment in front of it does *not* survive to the next
command in the list - the port would silently have been the compose file's default.
The environment now goes on the `docker compose` command itself, and on *every*
subcommand, so `ps` and `down` compute the same config `up` did.

`shellcheck` is not installed on this Mac; ran it as
`docker run --rm koalaman/shellcheck:stable`. **Clean.** The only two findings were
SC2016 on `DIR='$HOME/src/screeny'` / `BARE='$HOME/srv/screeny.git'`, where the
single quotes are the whole point (that `$HOME` is the *remote* shell's and must
arrive unexpanded); disabled inline with that reason. `bash -n` clean too.

Dry runs of both routes print a complete, correct plan and run nothing; the
`git remote add` on the workbench route is printed but not executed under
`--dry-run`.

### Step 6 - amd64: the half that could be proved here, and the half that could not

The target that matters is x86_64 Linux and this is an arm64 Mac, so a
`docker build --platform linux/amd64` went in under QEMU with a 45-minute cap.
**It failed at 584 s**, and the failure is the emulator's, not the Dockerfile's:

```
error: linking with `cc` failed: signal: 11 (SIGSEGV) (core dumped)
  ... -o /src/target/release/deps/libdocument_features-*.so ... -fuse-ld=lld
error: could not compile `document-features` (lib)
```

`lld` segfaulting while linking a proc-macro `.so` under `qemu-x86_64` is a known
emulation limit. Working around it would mean changing the linker for everyone to
please an emulator this project does not target, so it was not done. **x86_64 remains
to be verified on workbench itself**, which is the first thing the deployment does.

What the attempt *did* prove before it died, which is most of the arch risk:

- `rust:1-trixie` and `debian:trixie-slim` both resolve and pull for `linux/amd64`
  (the same digests are multi-arch manifests).
- The `rustup toolchain install stable` layer works on amd64 (19.7 s).
- **The whole runtime stage completed on amd64 in 68.8 s**:
  `libvulkan1:amd64 1.4.309.0-1`, `vulkan-tools 1.4.304.0+dfsg1-1`,
  `mesa-vulkan-drivers:amd64 25.0.7-2+deb13u1`. Mesa 25.0.7 is comfortably new enough
  for Raptor Lake ANV, which is the thing workbench actually needs.
- 160+ crates compiled for amd64, wgpu 30 and naga among them, before the linker
  crashed on a *host* proc-macro.

Nothing in the Dockerfile is architecture-specific; there is no `--platform`, no
pinned digest, no arch in any path.

### Step 7 - the things the dry runs and the local container caught

Beyond the `cd`-builtin bug in step 4:

- **The push the script advises has to be possible.** On the workbench route the
  refusal says `git push workbench main`, but the bare repo it pushes to is created
  by the script - and that creation used to happen *after* the refusal, so the advice
  was unfollowable on a first run. The reachability check and the bare-repo creation
  now come first. It is three runs of the script on that route, and the doc says so.
- **`restart: unless-stopped` was checked, not assumed.** `docker kill` does not test
  it (Docker treats any manual stop as "stopped"); a throwaway container given a bad
  flag exits 1 and was restarted **7 times in 8 s**. Which also means a typo'd
  argument presents as a crash loop, so the doc says to reach for `--logs` first.
- **The `$${SCREENY_PORT}` healthcheck escaping is the tested one.** The portable
  file was changed to use the same form as the workbench file rather than a literal
  port, and the container goes healthy with it - so the escaping the untestable file
  depends on is escaping that has actually run.
- **`screeny discover` inside the container prints macOS advice.**
  `crates/screeny/src/error.rs` `Error::hint()` has no `cfg`, so a Linux container is
  told to open System Settings and run `dns-sd`. That is verification (a)'s own
  command, so the first empty browse on workbench would send the operator to a
  settings panel that does not exist. Written up as **card 147**;
  `crates/screeny/**` belongs to card 092, so nothing there was touched. The doc
  warns about it and gives `avahi-browse -rt _screeny._udp` on the host as the real
  equivalent.
- `cargo test --release --no-fail-fast` at the root: **300 passed, 0 failed**, 52
  test binaries. No Rust was changed by this card.

### Step 8 - the doc

`docs/design/deployment.md` is the operational half of `studio-vision.md`: the file
table, "there is no password" first, the ordered first-deployment runbook for the
GitHub route, the no-GitHub route, the three verification one-liners with what good
output looks like *and* what to do when the answer is no, day-to-day
(`--status`/`--logs`/redeploy, Portainer), rollback and total removal, the portable
profile and why it is a whole file, the knobs table, and the two things the container
cannot fix (no state file and `SCREENY_LISTEN` ignored until card 106). The root
`README.md` points at it from the layout table and from a deployment line.

### Step 9 - what is proven, and what the orchestrator still has to do

| deliverable | here | on workbench |
|---|---|---|
| `Dockerfile` builds | **yes**, arm64, cold 54 s / warm 1.2 s | x86_64 build: only the runtime stage was provable under QEMU (see step 6). First `docker compose build` over SSH is the test. |
| non-root, `/data` writable | **yes**, uid 10001, marker survived a restart | volume ownership is the same mechanism; nothing host-specific |
| `/healthz` and compose healthcheck | **yes**, `Up (healthy)`, same `$${SCREENY_PORT}` form as the workbench file | same |
| UI served from the binary | **yes**, `/`, `/main.js`, `/style.css`, bootstrap, frame | same |
| streams to a panel | **yes**, to `screeny-sim` on the host: 29.7-30.5 fps, PAL8_LZ, exact-indexed, 0 gaps/rejects | to the real panel at 192.168.7.221 |
| SIGTERM releases the panel | **yes**, 0.49 s, exit 0, sim logged `Final` | same |
| capped logs, `unless-stopped` | **yes**, inspected; crash-restart proved with a throwaway | plus the reboot, which is the card's acceptance and only testable there |
| `network_mode: host`, `/dev/dri`, `group_add` | **no** - macOS has none of them. File renders correctly (`docker compose config`). | (a) and (b) below |
| mDNS beside avahi | **no** - no multicast in a Mac VM | (a) below |
| GPU adapter | **partly**: Vulkan works in the image and the studio logged `gpu=llvmpipe ... backend=Vulkan`; only the adapter differs on workbench | (b) below |
| local time | **yes**, `PDT` inside the container | (c) below - workbench's own zone is `Etc/UTC`, so this is the one that would silently be wrong |
| deploy script | **dry runs of both routes**, every refusal path exercised against a throwaway repo, shellcheck clean | the real run |

The three one-liners, and what a "no" means, are in `docs/design/deployment.md`,
"After the deployment: three questions, three one-liners".

### Step 10 - bench hygiene

Everything this card started on the local Docker was inside the project
`screeny107` or named `screeny-107-*`, and all of it is removed: the compose stack,
its network and its `screeny107_state` volume, the `screeny-107-cpuonly` image and
the throwaway restart-policy container. The `koalaman/shellcheck:stable` image pulled
to lint the script is removed too. Left behind on purpose: `screeny-studio:portable`
(491 MB) and the BuildKit cache mounts, which are what make the next build 1.2 s
instead of 54 s. The `screeny-sim` used for the streaming test was bounded with
`--exit-after 60` and exited on its own.
