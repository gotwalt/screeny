# Screeny Studio in a container.
#
#   docker compose -p screeny build
#
# Two stages. The builder is the official Rust image and keeps cargo's registry
# and the workspace `target/` in BuildKit cache mounts, so the second and later
# builds on a machine are incremental (minutes rather than tens of minutes).
# The runtime is plain Debian plus Mesa's Vulkan driver, because the GPU pieces
# go through wgpu and workbench's adapter is an Intel iGPU (ANV).
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
    strip /out/screeny-studio /out/screeny

# ---------------------------------------------------------------- runtime ---
FROM ${RUNTIME_IMAGE} AS runtime

# libvulkan1 + mesa-vulkan-drivers give wgpu the Intel ANV driver on workbench
# (and lavapipe, Mesa's software rasteriser, on a host with no usable adapter -
# slow, but 64x32 is small). vulkan-tools is `vulkaninfo`, which is how the
# "does the container see the GPU?" question gets a one-line answer; it is a
# couple of megabytes and can be dropped once that stops being interesting.
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

USER studio
WORKDIR /data

ENV TZ=America/Los_Angeles \
    SCREENY_STATE_DIR=/data \
    SCREENY_LISTEN=0.0.0.0:8787 \
    RUST_BACKTRACE=1

EXPOSE 8787

# The compose files pass `--listen` explicitly as well, because the binary does
# not read SCREENY_LISTEN yet (card 106). Once it does, this is the whole story.
ENTRYPOINT ["/usr/local/bin/screeny-studio"]
CMD ["--listen", "0.0.0.0:8787"]
