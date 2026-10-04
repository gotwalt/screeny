# Screeny Studio in a container.
#
#   docker compose -p screeny build
#
# Two stages. The builder is the official Rust image and keeps cargo's registry
# and the workspace `target/` in BuildKit cache mounts, so the second and later
# builds on a machine are incremental (minutes rather than tens of minutes).
# The runtime is plain Debian plus Mesa's Vulkan driver, because the GPU pieces
# go through wgpu and the author's Linux host has an Intel iGPU (ANV).
#
# The build needs no secrets of any kind: it compiles host crates only. WiFi
# credentials belong to `firmware/`, which `.dockerignore` keeps out of the
# context entirely.
#
# Build arguments:
#   FEATURES      cargo features for screeny-studio. Empty (the default) means
#                 the crate's defaults, which include `gpu`. Pass
#                 `--build-arg FEATURES=none` for a CPU-only studio with no
#                 graphics driver compiled in at all.
#   RUST_IMAGE    override the builder base, e.g. to pin a Rust version.
#   FIRMWARE_VERSION
#                 the panel firmware this image offers over the air (card 364):
#                 the release `fw-v<version>`'s `screeny-fw-<version>.bin`,
#                 downloaded at build time and checked against SHA256SUMS.
#                 Bump it to offer a newer one.

ARG RUST_IMAGE=rust:1-trixie
ARG RUNTIME_IMAGE=debian:trixie-slim

# ---------------------------------------------------------------- builder ---
FROM ${RUST_IMAGE} AS builder

# `rust-toolchain.toml` in the workspace says `channel = "stable"`, which is not
# the name the official image installs the compiler under, so rustup would fetch
# a whole toolchain during the build step and do it again whenever the source
# changes. Install it here instead: its own layer, cached, source-independent.
RUN rustup toolchain install stable --profile minimal --no-self-update \
 && rustup default stable \
 && rustc --version

WORKDIR /src

# The manifests first: this layer changes only when a dependency changes, and
# with the registry cache mount below it means an edit to a piece does not
# re-resolve the graph.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates

ARG FEATURES=
# `--locked` so the image is built from the Cargo.lock in the tree and a build
# can never silently pick up a different dependency version.
#
# The binary has to be copied out inside this RUN: a cache mount is not part of
# the layer, so anything left in /src/target disappears when the step finishes.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    set -eux; \
    case "${FEATURES}" in \
      ""|"default") feat="" ;; \
      "none")       feat="--no-default-features" ;; \
      *)            feat="--no-default-features --features ${FEATURES}" ;; \
    esac; \
    cargo build --locked --release -p screeny-studio -p screeny ${feat}; \
    mkdir -p /out; \
    cp target/release/screeny-studio /out/; \
    cp target/release/screeny /out/; \
    strip --strip-debug /out/screeny-studio /out/screeny; \
    ls -l /out/
# `--strip-debug`, not a full strip. The workspace builds release with
# `debug = 1`, and keeping all of it costs ~90 MB in the image; throwing all of
# it away costs the names in a panic backtrace, which is a bad trade for a
# service meant to run unwatched for months. `--strip-debug` drops the DWARF and
# keeps `.symtab`, so a backtrace still names the functions - no line numbers,
# but never a column of hex.

# --------------------------------------------------------------- firmware ---
# Card 364: the firmware this Studio offers to panels, fetched at build time
# from the GitHub release `fw-v<FIRMWARE_VERSION>` and checked against that
# release's SHA256SUMS. It is the **app image** (`screeny-fw-<v>.bin`, the one
# `POST /api/v1/firmware` takes), not the `-full.bin` a first install flashes.
# There is no internet at runtime: an image update is what offers a firmware
# update (docs/design/home-assistant-app.md, decision 9). Runs on the build
# machine's own architecture - it only downloads a file - so a multi-arch build
# does not emulate it.
FROM --platform=$BUILDPLATFORM ${RUNTIME_IMAGE} AS firmware
ARG FIRMWARE_VERSION=0.10.0
ARG FIRMWARE_REPO=gotwalt/screeny
RUN set -eux; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates curl; \
    rm -rf /var/lib/apt/lists/*
WORKDIR /fw
RUN set -eux; \
    img="screeny-fw-${FIRMWARE_VERSION}.bin"; \
    base="https://github.com/${FIRMWARE_REPO}/releases/download/fw-v${FIRMWARE_VERSION}"; \
    curl -fsSL --retry 3 -o SHA256SUMS "${base}/SHA256SUMS"; \
    curl -fsSL --retry 3 -o "${img}" "${base}/${img}"; \
    test "$(grep -cF "  ${img}" SHA256SUMS)" = 1; \
    grep -F "  ${img}" SHA256SUMS | sha256sum -c -; \
    mv "${img}" screeny-fw.bin; \
    ls -l screeny-fw.bin

# ------------------------------------------------------------------ base ---
# Everything the two images below share. Never built on its own.
FROM ${RUNTIME_IMAGE} AS base

# libvulkan1 + mesa-vulkan-drivers give wgpu the Intel ANV driver on a host
# with an Intel iGPU (and lavapipe, Mesa's software rasteriser, on a host with
# no usable adapter - slow, but 64x32 is small). vulkan-tools is `vulkaninfo`,
# which is how the "does the container see the GPU?" question gets a one-line
# answer; it is a couple of megabytes and can be dropped once that stops being
# interesting.
# curl is the healthcheck and the way to poke the API from inside the container.
# tzdata so TZ means something: the clock pieces show local time.
RUN set -eux; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates \
        tzdata \
        curl \
        libvulkan1 \
        mesa-vulkan-drivers \
        vulkan-tools; \
    rm -rf /var/lib/apt/lists/*

# A fixed, unprivileged uid. It owns /data, so an empty named volume mounted
# there is created with that ownership and the studio can write its state file.
# Access to /dev/dri comes from the compose file's `group_add`, not from here:
# the render gid is a property of the host, not of the image.
RUN set -eux; \
    groupadd --system --gid 10001 studio; \
    useradd --system --uid 10001 --gid 10001 --home-dir /data --shell /usr/sbin/nologin studio; \
    mkdir -p /data; \
    chown studio:studio /data

COPY --from=builder /out/screeny-studio /usr/local/bin/screeny-studio
COPY --from=builder /out/screeny /usr/local/bin/screeny
# Card 364: where `screeny_studio::firmware::DEFAULT_PATH` looks.
COPY --from=firmware /fw/screeny-fw.bin /usr/local/share/screeny/screeny-fw.bin

WORKDIR /data

ENV TZ=America/Los_Angeles \
    SCREENY_STATE_DIR=/data \
    SCREENY_LISTEN=0.0.0.0:8787 \
    RUST_BACKTRACE=1

EXPOSE 8787

# No CMD: the binary reads SCREENY_LISTEN and SCREENY_STATE_DIR (card 106), and a
# flag here would silently beat the environment a compose file sets.
ENTRYPOINT ["/usr/local/bin/screeny-studio"]

# ---------------------------------------------------------------------- app ---
# The Home Assistant app image (card 359; `ha-app/`): `docker build --target
# app`. The Supervisor runs an app's container as the image's user and mounts
# `/data` root-owned, so this one stage runs as root. Nothing else differs; in
# particular it listens as `SUPERVISOR_TOKEN` and `/data/options.json` say
# (`crates/studio/src/app.rs`), not as `SCREENY_LISTEN` says.
FROM base AS app
USER root
# Replaces the obsolete `watchdog` in ha-app/config.yaml. App mode listens only
# on the hassio gateway (`app::INGRESS_ADDR`, ingress port 8099), and
# `GET /healthz` is the one path its peer check lets this container reach.
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl -fsS -o /dev/null http://172.30.32.1:8099/healthz || exit 1

# ---------------------------------------------------------------- runtime ---
# The default target - the last stage - and what both compose files build:
# unprivileged, exactly as before.
FROM base AS runtime
USER studio

