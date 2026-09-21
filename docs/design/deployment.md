# Deploying Screeny Studio

Screeny Studio is one Rust binary that serves its own web UI and streams frames to
the panel. This is how it becomes a service on `workbench.local` that nobody has to
log in to: a container, started by Docker on boot, restarted if it dies, showing the
same patch it was showing before the machine went down.

The vision and the survey of the host are in
[`studio-vision.md`](studio-vision.md). This file is the operational half: what the
files are, how to deploy, what to check, and how to take it all away again.

| file | what it is |
|---|---|
| `Dockerfile` | two stages. Builder: the official Rust image, cargo registry and `target/` in BuildKit cache mounts. Runtime: `debian:trixie-slim` + Mesa's Vulkan driver, `screeny-studio` and the `screeny` CLI, running as uid 10001. |
| `docker-compose.yml` | the service on a Linux host with a GPU: host networking, `/dev/dri`, a named state volume, healthcheck, capped logs, `restart: unless-stopped`. |
| `docker-compose.portable.yml` | the same service on a Mac or on any host with no `/dev/dri` and no host networking: a published port, no GPU, discovery not expected to work. A whole file, not an override - see below. |
| `tools/deploy-workbench.sh` | does the deployment over SSH, and `--status`, `--logs`, `--down` afterwards. |
| `.dockerignore` | keeps `target/`, `.git/`, `docs/`, `.claude/`, `captures/`, `backup/` and **all of `firmware/`** out of the build context. |

## Before anything else: there is no password

The studio has no authentication (parked card 041). On workbench it listens on every
interface, which is the owner's choice for a machine on the home LAN and the tailnet:
anyone who can reach `workbench.local:8787` can change what is playing and can point
the studio at any panel on the network.

That is fine, and it is also the whole security model. **Do not publish the port
further.** No reverse proxy, no port forward, no tunnel, no firewall change. If this
ever needs to leave the LAN, it needs card 041 first.

## The first deployment, over the GitHub route

The host pulls the private repo from GitHub itself (it authenticates as `gotwalt`).
The owner allowed `main` to be pushed to that repo on 2026-09-20.

```bash
# 1. On the bench Mac, from the screeny checkout. Read the plan first; this
#    prints every local and remote command and runs none of them.
tools/deploy-workbench.sh --dry-run

# 2. Push main. The script will never do this for you: it is a decision.
git push origin main

# 3. Deploy. Clones into ~/src/screeny on the host if it is not there, checks
#    out origin/main, builds, starts, waits for /healthz.
tools/deploy-workbench.sh

# 4. Look at it.
open http://workbench.local:8787/
```

The first build is slow - it is Rust plus wgpu plus a Mesa install - and it happens
on the host, over SSH, not inside Portainer. Later builds reuse the cargo registry
and `target/` cache mounts and are incremental.

If `main` is ahead of `origin/main`, step 3 stops before touching the host and says
`git push origin main`. That is on purpose: deploying an old commit silently is worse
than not deploying.

### The route with no GitHub

For a host that cannot reach GitHub, or for a repo that is not allowed off the bench:

```bash
tools/deploy-workbench.sh --route workbench --dry-run   # read the plan
tools/deploy-workbench.sh --route workbench             # adds the remote, makes the
                                                        # bare repo, then refuses:
                                                        # "git push workbench main"
git push workbench main
tools/deploy-workbench.sh --route workbench             # and now it deploys
```

Three runs, not two, and that is the design rather than an accident: the script adds
the local `workbench` remote (`workbench.local:srv/screeny.git`) and creates the bare
repo behind it *before* it checks whether the branch is there, so that the push it
tells you to run is a push that will work. It will never repoint an existing
`workbench` remote, and it never creates or changes `origin`.

## After the deployment: three questions, three one-liners

These are the things worth knowing once, on the real host. Each is one command and
what good output looks like.

### (a) Does mDNS work inside a host-network container, next to the host's avahi?

```bash
ssh workbench.local -- docker exec screeny-studio screeny discover
```

**Good:** a line naming `screeny-4a00a4` at `192.168.7.221:49374`. The `mdns-sd`
crate binds UDP 5353 with `SO_REUSEPORT`, so it shares the port with the avahi daemon
that already owns it on the host.

**The advice it prints when it finds nothing is the Linux advice** since card 147:
the container on bridge networking, the multicast route, UDP 5353, and
`avahi-browse -rt _screeny._udp` for a second opinion. Run that one on the host
(`ssh workbench.local -- avahi-browse -rt _screeny._udp`): if the host sees the panel
and the container does not, the container is the problem.

**If it finds nothing:** discovery is a convenience, not a requirement. The studio
takes an address directly, so set the panel by address and carry on:

```bash
curl -fsS -X POST http://workbench.local:8787/api/v1/set_panel \
  -H 'content-type: application/json' -d '{"on":true,"to":"192.168.7.221"}'
```

Then write it up - it is the "avahi owns 5353" risk in `studio-vision.md` coming
true, and the answer is either browsing through avahi over D-Bus or living on
configured addresses. `avahi-resolve -n screeny-4a00a4.local` on the host answers
`192.168.7.221` either way, so the host is not the problem if the container is blind.

### (b) Does wgpu find the Intel iGPU inside the container?

```bash
ssh workbench.local -- docker exec screeny-studio vulkaninfo --summary
```

**Good:** a `GPU0` whose `deviceName` names Intel (Raptor Lake / Xe / UHD Graphics)
and whose `deviceType` is `PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU`, with
`driverName = intel_open_source_mesa_driver`. That is ANV, and wgpu will pick it up
through `WGPU_BACKEND=vulkan`.

The studio's own confirmation, since card 145, is **one line on stdout at startup**,
whichever way it went, and nobody has to select a patch first:

```bash
ssh workbench.local -- docker logs --tail 50 screeny-studio | grep 'studio: '
```

**Good:** `studio: gpu Intel(R) Graphics (RPL-P) (Vulkan)`. With no adapter it reads
`studio: no GPU adapter: ... - overland, lattice, knot cannot be played here`, and the
same fact is on `GET /api/v1/status` as `gpu` and on the page, where those three patches
are struck through with the reason under them. **A missing adapter is never a 503**:
the CPU patches are unaffected and no restart conjures a GPU.

```bash
ssh workbench.local -- curl -s localhost:8787/api/v1/status | jq .gpu
```

The older per-patch line on stderr is still there the first time a GPU patch opens:
`screeny-art: gpu=Intel(R) Graphics (RPL-P) backend=Vulkan`. (On this Mac,
in a container with no `/dev/dri`, the same line reads
`gpu=llvmpipe (LLVM 19.1.7, 128 bits) backend=Vulkan` - the code path is identical,
only the adapter differs.)

Don't try to POST that through `ssh` on one line: `ssh` hands its arguments to a
remote shell that re-parses them, and the JSON body's quotes do not survive.

**If the only device is `llvmpipe`** (`PHYSICAL_DEVICE_TYPE_CPU`), the container is
not reaching `/dev/dri`. Almost always the render gid: check it and redeploy with the
right one.

```bash
ssh workbench.local -- stat -c '%G %g' /dev/dri/renderD128     # expect: render 993
tools/deploy-workbench.sh --render-gid <gid>
```

lavapipe still renders - it is Mesa's software rasteriser and 64x32 is small - so the
GPU patches will work, slowly, rather than fail. **If there is no adapter at all**, the
GPU patches render black and say so once on stderr per patch
(`screeny-art: <patch>: no GPU adapter: ...; rendering black`) - and, since card 145,
the page says it too: `overland`, `lattice` and `knot` are struck through with "no GPU"
and the reason is the line under the list, so the fallback (stay on the CPU patches -
`clocks-numerals`, `clocks-dials`, `vesta`, `metaballs`, `flock`) is the obvious
thing to do rather than something to be told. The heavier alternative is to rebuild
with no graphics driver in the tree at all:

```bash
ssh workbench.local -- "cd ~/src/screeny && SCREENY_FEATURES=none SCREENY_PORT=8787 docker compose -p screeny -f docker-compose.yml build && SCREENY_FEATURES=none SCREENY_PORT=8787 docker compose -p screeny -f docker-compose.yml up -d"
```

(The `build` alone only makes the image; the `up -d` is what starts using it. Doing
this reverts on the next `tools/deploy-workbench.sh`, which builds with the default
features - set `SCREENY_FEATURES=none` in an `.env` beside the compose file on the
host if it should stick.)

### (c) Do the clock patches show local time?

```bash
ssh workbench.local -- docker exec screeny-studio date
```

**Good:** the owner's wall-clock time, with `PDT`/`PST` on the end. workbench's own
timezone is `Etc/UTC`, so this only works because the compose file sets
`TZ=America/Los_Angeles` and the image installs `tzdata`. Change it with
`tools/deploy-workbench.sh --tz <zone>` or `TZ=<zone>` in an `.env` beside the
compose file on the host.

**If it says UTC:** `tzdata` is missing from the image or `TZ` did not reach the
container. `docker exec screeny-studio printenv TZ` tells you which.

## Day to day

```bash
tools/deploy-workbench.sh --status     # docker compose ps, and /healthz
tools/deploy-workbench.sh --logs       # the last 200 lines
tools/deploy-workbench.sh              # redeploy whatever is on origin/main now
```

`restart: unless-stopped` means a crash is restarted and a `docker compose stop` is
not. It also means a **bad argument looks like a crash loop**: the studio exits 1 on
an unknown flag and Docker restarts it a few times a second. `--logs` shows the usage
text straight away if that is what has happened.

Portainer (`https://workbench.local:9443`) sees the stack as the `screeny` project
and can stop, start and inspect it. Do the *building* over SSH: a Rust + wgpu image is
far too slow to build inside Portainer, and Portainer's editor is not where a
`docker-compose.yml` should be edited when the repo has one.

### Rolling back, and removing every trace

```bash
tools/deploy-workbench.sh --down                       # stop and remove the container
tools/deploy-workbench.sh --down --volumes             # ... and forget the saved state

# roll back to an older commit: check it out on the host and rebuild there.
# Note that the next plain `tools/deploy-workbench.sh` moves it back to
# origin/main, which is usually what you want and is never a surprise.
ssh workbench.local -- "cd ~/src/screeny && git checkout <sha>"
ssh workbench.local -- "cd ~/src/screeny && SCREENY_PORT=8787 SCREENY_RENDER_GID=993 TZ=America/Los_Angeles docker compose -p screeny -f docker-compose.yml up -d --build"

# remove everything this ever created on the host
ssh workbench.local -- docker compose -p screeny -f ~/src/screeny/docker-compose.yml down --volumes
ssh workbench.local -- docker image rm screeny-studio:local
ssh workbench.local -- rm -rf ~/src/screeny
```

Nothing above touches another container, another volume, another network or the
Docker daemon's configuration, and nothing prunes. Five other compose projects live
on that host - `docker`, `homework`, `homework-work`, `june`, `scrypted` - and the
explicit `-p screeny` project name on every invocation is what keeps them apart.

## Running it somewhere else

`docker-compose.portable.yml` is for a Mac, or any host with no `/dev/dri` and no
host networking:

```bash
docker compose -p screeny -f docker-compose.portable.yml up -d --build
open http://localhost:8787/
```

It is a complete file rather than an override of `docker-compose.yml`, which is worth
knowing why: compose merges list-valued keys like `devices:` and `group_add:` by
appending, so an override can add a device but can never take one away. `-f base.yml
-f override.yml` would still try to open `/dev/dri` on a machine that has none.

What is different there:

- **No discovery.** On a Mac the engine runs inside a VM with no access to the LAN's
  multicast. Outbound unicast UDP is NATed and does work, so a configured address
  does: `host.docker.internal:49374` reaches a `screeny-sim` on the host. Since card
  146 that name is resolved rather than browsed for - anything with a dot or a port
  in it goes to the system resolver, a bare name is still an mDNS instance name - so
  it can be given to `set_panel` as it stands, and a name that means nothing says so
  instead of reporting a missing panel.
- **No GPU.** The GPU patches fall back to lavapipe, Mesa's software rasteriser, which
  the image carries. Build with `SCREENY_FEATURES=none` for a studio with the CPU
  patches only and no graphics driver compiled in at all.

## Knobs

All of these have defaults that are right for workbench; set them in an `.env` file
beside the compose file on the host, or pass the matching flag to the deploy script.

| variable | default | what it does |
|---|---|---|
| `SCREENY_PORT` | `8787` | the web port. 8787 is free on workbench; 8000, 8443, 9000, 9443, 3002, 5002, 1080 and 11434 are not. |
| `SCREENY_RENDER_GID` | `993` | the host's `render` group, which owns `/dev/dri/renderD128`. `stat -c %g /dev/dri/renderD128`. |
| `TZ` | `America/Los_Angeles` | what the clock patches call "now". |
| `SCREENY_FEATURES` | *(empty)* | cargo features for the build. `none` = no graphics driver at all, CPU patches only. |

An `.env` is host state, not repo state: it is in `.gitignore` and in
`.dockerignore`, and it must never hold a credential.

## Two things the container cannot fix

- **State and listen address come from the environment** since card 106: the studio
  writes `state.json` under `SCREENY_STATE_DIR=/data` (a named volume owned by uid 10001)
  and listens on `SCREENY_LISTEN`. A restart resumes what every panel was playing. The
  image has no `CMD` on purpose: a flag would silently beat the environment.
- **What it remembers survives deploys** (the owner's requirement, 2026-09-19): everything
  the studio remembers - devices, what each panel plays, how each patch was left tuned
  (card 165) - is in
  `/data/state.json` on the named volume `screeny_state`. A redeploy rebuilds the image and
  recreates the container but keeps the volume; verified on workbench (a panel playing
  `overland` seed 4242 was playing it again 4 s after a full redeploy). Only
  `tools/deploy-workbench.sh --down --volumes` deletes it.
